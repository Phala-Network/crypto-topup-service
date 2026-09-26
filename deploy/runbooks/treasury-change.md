# Treasury change

**Trigger:** an approved treasury migration, or a treasury Safe that is no longer acceptable. There
is no runtime treasury setter.

**Impact:** every forwarder is bound to the treasury through its implementation, so a new treasury
is a new factory and a new route version; the old version stays loaded for its deposits.

## First steps

For an emergency migration, pause every scope on the route and, if the Safe or the operator is
suspected compromised, have the admin Safe revoke `OPERATOR_ROLE`
([operator key compromise](operator-key-compromise.md)):

```sh
admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes","addresses","settlement","flush","refunds"]}'
cast call "$FACTORY" 'implementation()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$IMPLEMENTATION" 'treasury()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
```

## Decide

- Current Safe secure: plan a migration window.
- Current Safe compromised: keep every scope paused and invoke the Safe incident procedure.
- Any Safe or deployment verification fails: stop.

## Fix

**HUMAN-ONLY, Safe owner and deployer:** update `treasury` in
`deploy/contracts/safe-expectations.json` by reviewed PR and deploy and verify the new factory
exactly as [CONTRACTS.md](../CONTRACTS.md) describes (both providers). Grant the attested operator
`OPERATOR_ROLE` on the new factory through the Safe. Then add a new route version with the new
factory, implementation, and treasury, keep the old version, and Deploy `upgrade`
([CONTRACTS.md, "Route and compose update"](../CONTRACTS.md#route-and-compose-update)).

## Done when

Both providers return the new code, `implementation()`, `treasury()`, operator role, and a sample
`addressOf`; the attested compose carries the new route version; one small Sepolia deposit is
credited and flushed to the new treasury; the paused scopes are resumed. New-version addresses
cannot be rebound: rollback is a Deploy `upgrade` to the prior compose, with both versions kept.

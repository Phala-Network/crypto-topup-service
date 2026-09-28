# Treasury change

**Trigger:** an approved treasury migration, or a treasury Safe that is no longer acceptable. There
is no runtime treasury setter.

**Impact:** every forwarder's address commits to its treasury (the clone's only immutable
argument), so a new treasury is a new route version; addresses already issued keep paying the old
treasury, and the old version stays loaded for their deposits.

## First steps

For an emergency migration, pause every scope on the route. The service holds no key that can
move funds, and anyone can flush a forwarder, but only to the treasury its address commits to:

```sh
admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes","settlement","refunds"]}'
cast call "$FACTORY" 'addressOf(address,bytes32)(address)' "$TREASURY" "$SALT" --rpc-url "$RPC_PROVIDER_A_URL"
```

## Decide

- Current Safe secure: plan a migration window.
- Current Safe compromised: keep every scope paused and invoke the Safe incident procedure.
- Any Safe or deployment verification fails: stop.

## Fix

**HUMAN-ONLY, Safe owner:** update `treasury` in `deploy/contracts/safe-expectations.json` by
reviewed PR and verify the new Safe as [CONTRACTS.md](../CONTRACTS.md) describes (both providers).
The permissionless factory is shared, so no contract is deployed. Then add a new route version with
the new treasury, keep the old version, and Deploy `upgrade`
([CONTRACTS.md, "Route and compose update"](../CONTRACTS.md#route-and-compose-update)).

## Done when

Both providers return a sample `addressOf(new treasury, salt)` equal to the service's; the attested
compose carries the new route version; one small Sepolia deposit is credited and flushed to the new
treasury; the paused scopes are resumed. New-version addresses cannot be rebound: rollback is a
Deploy `upgrade` to the prior compose, with both versions kept.

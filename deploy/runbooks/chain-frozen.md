# Chain frozen

**Trigger:** `TopupReconciliationMismatch` with `check:address_derivation`; products report
`423 chain_frozen` from address issuance or rate-lock creation; `topup-scanner-<chain_id>` misses
its check-ins because the frozen chain's scanner has paused.

**Impact:** the factory's `addressOf(salt)` disagrees with a stored address, so the service can no
longer prove where that chain's deposits go. Until the freeze is lifted, the chain's deposits
wait, its scanner and head scan stop, the flusher plans nothing, and address issuance and
rate-lock creation answer `423`. Other chains keep running; credited facts are never rolled back.

## First steps

1. Read `subjects` (`chain_id`, `address_id`, `salt`), `expected.address` (the factory's), and
   `observed.address` (the stored one) from the Sentry event.
2. Ask the factory through both providers and check the contract tuple against the attested route:

   ```sh
   cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$RPC_PROVIDER_A_URL"
   cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$RPC_PROVIDER_B_URL"
   cast call "$FACTORY" 'implementation()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
   cast call "$IMPLEMENTATION" 'treasury()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
   ```

3. Tell the product that deposits on the chain are unavailable and must show no address
   ([incident communication](incident-communication.md)).

## Decide

- Providers disagree about `addressOf`: [provider disagreement](provider-disagreement.md) first.
- Factory, implementation, or treasury differs from the route: configuration or deployment
  incident; engage Security and Finance.
- Contracts match but the stored address differs: database corruption or tampering; preserve the
  Sentry event and escalate to Security.
- Funds already reached a stored address the factory does not derive: open a Finance and Security
  incident for them.

## Fix

There is no in-service repair and no API that lifts a freeze: the block is a database row only the
database owner can delete, and a production CVM offers no owner session. Escalate to Engineering.
Wrong contracts are corrected with a new route version; wrong stored rows by a restore to a point
before the corruption ([RESTORE.md](../RESTORE.md)). If any stored address still disagrees, the
next reconciliation round (every 10 minutes) freezes the chain again.

## Done when

A reconciliation round raises no new `address_derivation` finding, `topup-scanner-<chain_id>`
checks in again, and address issuance answers normally.

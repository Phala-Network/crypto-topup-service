# Operator key compromise

**Trigger:** an operator transaction that is not a flush, an unexpected pending nonce, the
`MissingConsumedReceipt` alert, an unexplained gas drop ([gas refill](gas-refill.md)), or evidence
that key material was exposed. `OperatorRoleMissing` (the flusher plans and sends nothing, every
maintenance tick, until the role is granted) is expected after a revocation and after a key-version
bump deployed before its grant.

**Impact:** the operator can spend its gas and call `ForwarderFactory.flush`, which pays only the
immutable treasury: unnecessary gas spend and forced flush timing on every factory where it holds
`OPERATOR_ROLE`.

## Confirmed compromise: revoke first

**HUMAN-ONLY, Finance Safe, before anything else:** revoke the role on every affected factory and
confirm `false` through both providers. The compromised key is never re-granted.

```sh
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
```

Then pause `flush` on every route of the chain
(`admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["flush"]}'`); the revocation is what
stops anyone else holding the key. Deposits keep being credited. Record the operator's nonce,
balance, and transaction history as evidence (`cast nonce`, `cast balance`, a block explorer).

Stop the CVM (**HUMAN-ONLY**, `npx --yes phala@1.1.22 cvms stop "$TOPUP_CVM_ID"`) only if the CVM
itself is suspected of leaking the key: that halts every route. Every `operator/v{n}` derives from
the same application key, so if the CVM or application key is suspected, a new version is equally
exposed: escalate to Security for a new application identity instead.

## Replace the operator

1. Set `chain.operator_key_version` to the next version in a new version of every current route
   on the chain (they must share it), merge the route PR, and Deploy `upgrade`.
2. Read the new operator address from a fresh, verified attestation
   ([deploy/README.md, "Flusher operator"](../README.md#flusher-operator)).
3. **HUMAN-ONLY, Finance Safe:** grant it `OPERATOR_ROLE` on every affected factory, confirm `true`
   through both providers, and fund it ([gas refill](gas-refill.md)).
4. The flusher re-binds unsigned plans to the new operator (its nonces start at 0) and resumes by
   itself; resume `flush`.

## Done when

The old role is `false` and the new one `true` on both providers, `OperatorRoleMissing` has
stopped, and a flush from the new operator confirms. A routine rotation without compromise follows
[deploy/README.md, "Flusher operator"](../README.md#flusher-operator).

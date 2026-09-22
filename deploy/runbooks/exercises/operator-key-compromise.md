# Operator key compromise exercise

Date: 2026-09-22.

Status: partial. The role commands, the stop check, and the service side of a key-version rotation
ran locally; Finance Safe execution is human-only.

G2 exercised once: [ ]

Role revocation rehearsal on the Anvil factory from [local setup](local-setup.md). The Anvil admin
EOA stands in for the Finance Safe and submits the exact calldata the runbook prints; the hash
matches the contract's `OPERATOR_ROLE()` constant. Providers A and B are the same node through
`127.0.0.1` and `localhost`:

```sh
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
```

```text
OPERATOR_ROLE=0x97667070c54ef182b0f5858b034beac1b6f3089aa2d3188bb1e8929f4fa9b929
grant status=0x1
hasRole A after grant: true
revoke calldata=0xd547741f97667070c54ef182b0f5858b034beac1b6f3089aa2d3188bb1e8929f4fa9b92900000000000000000000000090f79bf6eb2c4f870365e785982e1f101e93b906
revoke status=0x1
hasRole A after revoke: false
hasRole B after revoke: false
```

The runbook's no-new-`sent` flush check ran as `wp_d5_app` and produced an empty difference.
Stale-plan re-binding to a new operator and its `flush.plan_operator_rebound` audit row are covered
by the [flush exercise](flush-reverted-or-bisected.md) integration test.

The provisioning half ran against the local dstack simulator and in the Anvil/PostgreSQL test
`anvil_operator_key_version_bump_gates_on_role_and_rebinds_stale_plans`
(`crates/topup/tests/flusher.rs`). `topup attest --operator-key-version` reported distinct
operator addresses for versions 1 and 2 with the same settlement key:

```text
{"keyid":"settlement/v1","operator_keyid":"operator/v1","operator_address":"0x5d1eea53869175644122b0e5b76417178eecdf14"}
{"keyid":"settlement/v1","operator_keyid":"operator/v2","operator_address":"0x0ef4d450a6d3e3c4b4ca7f0c3985134cf852b6cb"}
```

The test runs the flusher task with `operator_key_version: 2` before and after the grant:
- Without the role, the task plans and sends nothing and raises `OperatorRoleMissing` every
  interval.
- After the grant, the stale v1 plan is re-bound to the v2 operator at nonce 0, then sent and
  confirmed by the running task.
- Revoking a role while its task runs leaves the queued plan unsent.

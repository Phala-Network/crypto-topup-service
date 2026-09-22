# Operator key compromise exercise

Date: 2026-09-22.

Status: blocked on operator/v2 support in
[#60](https://github.com/Phala-Network/crypto-topup-service/issues/60) and human Finance Safe
execution. The role commands and the stop check were rehearsed locally.

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
by the [flush exercise](flush-reverted-or-bisected.md) integration test. Starting the service with a
replacement operator key is impossible until #60 lands, so the provisioning half is blocked.

# Treasury change exercise

Date: 2026-09-22.

Status: partial. The deployment steps are human-only Finance Safe work.

G2 exercised once: [ ]

The A2 tooling now exists, but refuses to run until Finance commits the Safe expectations, as the
runbook requires:

```sh
deploy/contracts/verify-safe.sh --rpc sepolia/a=http://127.0.0.1:8547
```

```text
error: expectations are not configured; Finance must commit the approved networks, admin and treasury Safes, owners, threshold, proxy code hashes, and singleton
```

After #72 added `rate_lock.max_creations_per_minute`, the committed template validates again:

```sh
topup route validate --template deploy/config/routes/phala-cloud-sepolia-pha.yaml
```

```text
route template `deploy/config/routes/phala-cloud-sepolia-pha.yaml` is valid at schema level; on-chain deployment and Safe control were not checked
exit=0
```

A copy with the Anvil contract tuple validated in [local setup](local-setup.md). The runbook's pause
verification (the `sent`-id snapshot, pause time, and `flush.send_paused` query) ran as `wp_d5_app`
against the migrated schema, and the `flush` pause behavior itself is covered by the [flush
exercise](flush-reverted-or-bisected.md). The contract tuple reads (`implementation()`,
`treasury()`) ran against the Anvil factory in the [chain frozen exercise](chain-frozen.md). New
factory deployment, the operator role grant, and compose approval are human-only and were not run.

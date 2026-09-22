# Treasury change exercise

Date: 2026-09-22.

Status: partial. The deployment steps are human-only Finance Safe work, and the committed route
template no longer validates on `main`.

G2 exercised once: [ ]

The A2 tooling now exists, but refuses to run until Finance commits the Safe expectations, as the
runbook requires:

```sh
deploy/contracts/verify-safe.sh --rpc sepolia/a=http://127.0.0.1:8547
```

```text
error: expectations are not configured; Finance must commit the approved networks, admin and treasury Safes, owners, threshold, proxy code hashes, and singleton
```

The committed template fails validation because C10 made `rate_lock.max_creations_per_minute`
required:

```sh
topup route validate --template deploy/config/routes/phala-cloud-sepolia-pha.yaml
```

```text
route file `deploy/config/routes/phala-cloud-sepolia-pha.yaml` is invalid: invalid route YAML: error: line 62 column 3: missing field `max_creations_per_minute`
  --> <input>:62:3
exit=1
```

A corrected copy validated in [local setup](local-setup.md). The runbook's pause verification (the
`sent`-id snapshot, pause time, and `flush.send_paused` query) ran as `wp_d5_app` against the
migrated schema, and the `flush` pause behavior itself is covered by the
[flush exercise](flush-reverted-or-bisected.md). The contract tuple reads (`implementation()`,
`treasury()`) ran against the Anvil factory in the [chain frozen exercise](chain-frozen.md). New
factory deployment, the operator role grant, and compose approval are human-only and were not run.

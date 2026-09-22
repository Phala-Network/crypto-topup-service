# Gas refill exercise

Date: 2026-09-22.

Status: partial; the refill itself is a human-only Finance Safe transfer.

G2 exercised once: [ ]

The runbook's balance and nonce reads against the Anvil node from [local setup](local-setup.md),
through both provider URLs, for the rehearsal operator address:

```sh
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
cast nonce "$OPERATOR_ADDRESS" --block pending --rpc-url "$RPC_PROVIDER_A_URL"
```

```text
10000000000000000000000
10000000000000000000000
0
```

The runbook's flush query ran as `wp_d5_app` against the migrated schema (see
[local setup](local-setup.md#runbook-sql)); the `flush` pause it relies on is covered by the
[flush exercise](flush-reverted-or-bisected.md). The Safe transfer and its receipt check are
human-only and were not run.

# Provider disagreement exercise

Date: 2026-09-22. The local compose intentionally points both RPC variables at
`http://127.0.0.1:1`.

Status: partial; blocked on a controllable dual-provider chain/sanctions fixture.

G2 exercised once: [ ]

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml logs --no-color topup
```

Observed repeated bounded retry evidence:

```text
finalized chain scan failed transiently
chain_id=11155111 error_category=chain_read retry_after_seconds=13
```

The read-only deposits query returned zero rows. Finalized-block comparison was not feasible because
the local stack has no EVM provider.

The sanctions precedence was exercised separately against the real screening implementation:

```sh
cargo test --locked -p topup sanctions_truth_table_maps_to_step_results -- --nocapture
```

Observed:

```text
test steps::screen::tests::sanctions_truth_table_maps_to_step_results ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 37 filtered out
```

That truth table covers every `Clear`/`Sanctioned`/`Unavailable` pair and verifies that any
`Sanctioned` result rejects while `Unavailable` retries only in the absence of a sanctions hit.
The chain-evidence disagreement half remains blocked, so G2 stays unchecked.

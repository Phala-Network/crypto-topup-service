# Provider disagreement exercise

Date: 2026-09-22.

Status: partial. The runbook's provider comparison, SQL, sanctions truth table, and the confirm
step's disagreement handling ran locally; a pump-driven scenario with two controllable,
disagreeing chain providers and sanctions oracles is not available.

G2 exercised once: [ ]

The runbook's comparison against two independent Anvil nodes (`8547` and a second node on `8548`
with 80 extra blocks) standing in for providers A and B:

```sh
cast block finalized --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {number,hash}'
cast block finalized --json --rpc-url "$RPC_PROVIDER_B_URL" | jq '(.data // .) | {number,hash}'
```

```text
{"number":"0x9","hash":"0xec92aec49a06df3427f5956a4ef85d4d6f58b5dd1bff9889c75d748e8f976002"}
{"number":"0x10","hash":"0x1aa9aa07039d73b4205111a88f87feaf96995e04642c1aa531e9a39b90b737a3"}
```

Foundry 1.8.3 wraps `cast block --json` in a `data` envelope; the runbook's filter handles both
shapes. The runbook's deposit query ran as `wp_d5_app` against the migrated schema (see
[local setup](local-setup.md#runbook-sql)).

The confirm step's disagreement behavior and the sanctions truth table ran against the real
implementations:

```sh
cargo test --locked -p topup --lib steps::confirm::tests:: -- --nocapture
cargo test --locked -p topup --lib sanctions_truth_table_maps_to_step_results -- --nocapture
```

```text
test steps::confirm::tests::provider_disagreement_retries ... ok
test steps::confirm::tests::wrong_provisional_block_is_corrected_from_receipt_identity ... ok
test steps::confirm::tests::agreed_canonical_evidence_corrects_provisional_row ... ok
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 23 filtered out; finished in 2.08s
test steps::screen::tests::sanctions_truth_table_maps_to_step_results ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 38 filtered out; finished in 0.00s
```

`provider_disagreement_retries` asserts a retry when providers disagree on the log;
`agreed_canonical_evidence_corrects_provisional_row` corrects a provisional row when both agree on
different evidence. The truth table covers every `Clear`/`Sanctioned`/`Unavailable` pair: any
`Sanctioned` rejects before pause state is considered, and `Unavailable` retries only without a
hit. The pump-level chain-evidence scenario remains blocked, so G2 stays unchecked.

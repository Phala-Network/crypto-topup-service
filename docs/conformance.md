# Product settlement conformance

`topup-conformance` is the executable contract test for product settlement endpoints described in
`docs/architecture.md` section 11. It sends real RFC 9421 requests through the production
`topup-adapters` settlement client and writes a versioned JSON report suitable for a CI artifact.

## Product test configuration

Run the product endpoint in an isolated test environment with:

- the ed25519 public key matching the suite seed, pinned together with the configured `keyid`;
- an empty idempotency and product-ledger namespace which is not shared with production;
- `conformance-accepted`, `conformance-refused`, and `conformance-processing` accounts, or pass
  different ids with the corresponding CLI flags;
- the stated per-deposit cap;
- when testing chain evidence, a route named `conformance`, version `1`, using persistent address
  version `1` and the Anvil factory, implementation, and token addresses printed by the suite.

The refused account must return `200 {"status":"rejected","reason":"..."}` without a ledger
credit. The processing account must return `200 {"status":"processing"}` and retain that answer
for `GET`. The accepted account must be creditable. These are test-fixture behaviors, not
production account names.

The literal signing-key value `dev` uses the public test seed `[7; 32]`. A file may instead contain
exactly 32 raw seed bytes or 64 hexadecimal characters. Never use a production settlement seed.

## Run

Without Anvil, obligation 5 is reported as `skipped`; the other protocol and product obligations
still run against deterministic fixture evidence:

```sh
cargo run --locked -p topup-conformance -- \
  --settlement-url http://127.0.0.1:8080/settlements \
  --signing-key dev \
  --keyid settlement/v1 \
  --per-deposit-cap 10000 \
  --report target/conformance-report.json
```

For the full chain-evidence check, start a disposable Anvil chain and pass its RPC URL. `forge` and
`cast` must be on `PATH`. The suite deploys the checked-in A1 `ForwarderFactory` and
`MockERC20`, prints their addresses, creates real Transfer logs to product-computed forwarders,
and mines 65 blocks before submitting finalized cases:

```sh
anvil --chain-id 31337 --slots-in-an-epoch 1

cargo run --locked -p topup-conformance -- \
  --settlement-url http://127.0.0.1:8080/settlements \
  --signing-key dev \
  --keyid settlement/v1 \
  --per-deposit-cap 10000 \
  --anvil-rpc http://127.0.0.1:8545 \
  --chain-id 31337 \
  --report target/conformance-report.json
```

The product test process must use its own RPC connection and configure the printed addresses as
the approved route. In automated E2 tests, start or reload the product fixture after those values
are available, or invoke the Rust suite and product fixture from the same test harness. The suite
does not call a product administration API.

Exit status is zero only if every non-skipped assertion passes. The report is written to `--report`
and also printed to stdout. No seed, signature, or raw authorization header is included.

## Assertions

The report contains these stable test ids:

| Test id | Assertion |
|---|---|
| `accepted` | A valid signed request returns `200 accepted` with a non-empty `destination_tx_id`, and immediate `GET` proves the record was committed before the answer. |
| `replay` | Exact replay returns the same destination id. The optional `GET /__conformance/ledger/{key}` probe is used when present; otherwise authoritative `GET` equality is used. |
| `payload_mismatch` | The same key with a changed payload returns `422`. |
| `concurrency` | Eight identical requests produce one destination id; other answers may be `409` or the same accepted answer, and an exposed ledger probe must report one mutation. |
| `get_original` | `GET` returns the terminal status, destination id, and the original payload byte-equal after JSON encoding. |
| `authentication` | Invalid signature bytes, a valid signature under the wrong `keyid`, an expired `created`, and a tampered body/digest are each rejected with `401` or `403`. |
| `idempotency_coverage` | A request carrying `Idempotency-Key` but omitting it from the covered signature components is rejected. |
| `per_deposit_cap` | `cap + 1` minor units is rejected by the product's independent cap. Product implementations must also enforce their configured per-period caps, although the suite cannot infer an arbitrary reset window from this endpoint contract. |
| `chain_evidence` | Missing log, wrong emitter, wrong `to`, wrong amount, and non-finalized evidence are rejected; a finalized real log is accepted. |
| `deposit_identity` | A key not equal to `deposit:` plus the UUIDv5 recomputed from chain id, transaction hash, and log index is rejected. |
| `business_refusal` | Product policy refusal is a typed `200 rejected` answer with a reason. |
| `processing` | A typed `200 processing` answer is retained by `GET`. |
| `unknown_get` | An unknown key returns `404` or the documented `200 {"status":"unknown"}` form. |

Together these exercise the six product obligations: pinned signature verification; lifetime
idempotency; commit-before-accepted and one mutation under concurrency; independent product caps;
independent finalized-log verification; and deterministic deposit-id recomputation.

## Report format

The schema version is currently `1`:

```json
{
  "version": 1,
  "settlement_url": "http://127.0.0.1:8080/settlements",
  "started_at": "2026-09-22T00:00:00Z",
  "finished_at": "2026-09-22T00:00:07Z",
  "passed": true,
  "summary": { "passed": 13, "failed": 0, "skipped": 0 },
  "tests": [
    {
      "id": "accepted",
      "name": "valid request is durably accepted",
      "status": "pass",
      "evidence": { "destination_tx_id": "credit-1", "committed_before_response": true }
    }
  ]
}
```

`status` is `pass`, `fail`, or `skip`. Evidence is intentionally bounded to response codes,
destination ids, counters, reasons, and assertion details.

## Reference endpoint

The in-memory reference endpoint is runnable documentation for E2:

```sh
cargo run --locked -p topup-conformance --bin topup-conformance-reference -- \
  --listen 127.0.0.1:8089 \
  --signing-key dev \
  --keyid settlement/v1 \
  --per-deposit-cap 10000
```

Use `--broken signature|idempotency|concurrency|caps|evidence|deposit-identity` to demonstrate that
the corresponding suite assertion fails. With the `postgres` feature, `--database-url` selects an
atomic PostgreSQL find-or-create store instead of memory:

```sh
cargo run --locked -p topup-conformance --all-features \
  --bin topup-conformance-reference -- \
  --database-url postgres://postgres:postgres@127.0.0.1/postgres
```

For RPC-backed reference verification, also pass `--anvil-rpc`, `--chain-id`,
`--asset-contract`, `--factory`, and `--implementation` using the fixture values printed by the
suite.

The production `Dockerfile` intentionally excludes both conformance binaries. They are integration
and CI tools; the distroless runtime image continues to contain only `/usr/local/bin/topup`.

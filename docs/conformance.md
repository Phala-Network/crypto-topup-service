# Product settlement conformance

`topup-conformance` is the executable contract test for product settlement endpoints described in
`docs/architecture.md` section 11. It sends real RFC 9421 requests through the production
`topup-adapters` settlement client, backs every request with a real Transfer log on a disposable
chain, and writes a versioned JSON report suitable for a CI artifact.

A product passes only when every case is `pass`. A case whose required observation is missing
(the ledger hook or the restart hook below) is reported as `incomplete`, which is never a pass.

## Phases

The suite runs in two phases so the product can be configured in between.

1. `prepare` connects to a disposable development chain (Anvil or equivalent) with an unlocked,
   funded account, deploys the A1 `ForwarderFactory` and two `MockERC20` tokens with `forge
   create` (or adopts pre-deployed ones), and writes a manifest.
2. Configure the product test environment from the manifest, start it, then `run` reads the same
   manifest, mints fresh Transfer logs for every request, and exercises the endpoint.

```sh
anvil --chain-id 31337

topup-conformance prepare \
  --rpc-url http://127.0.0.1:8545 \
  --chain-id 31337 \
  --manifest target/conformance-manifest.json

# configure and start the product from the manifest

topup-conformance run \
  --manifest target/conformance-manifest.json \
  --settlement-url http://127.0.0.1:8080/settlements \
  --signing-key dev \
  --keyid settlement/v1 \
  --per-deposit-cap 10000 \
  --per-period-cap 50000 \
  --period-seconds 86400 \
  --restart-command './scripts/restart-product.sh' \
  --report target/conformance-report.json
```

`prepare` accepts `--factory`, `--asset-contract`, and `--unapproved-asset-contract` to adopt
pre-deployed contracts; anything not supplied is deployed. Supplied tokens must expose a public
`mint(address,uint256)`. `--contracts-dir` points at the Foundry project (default: this
repository's `contracts/`). The manifest looks like:

```json
{
  "version": 1,
  "chain_id": 31337,
  "rpc_url": "http://127.0.0.1:8545",
  "product_slug": "conformance",
  "route": "conformance",
  "route_version": 1,
  "address_version": 1,
  "factory": "0x…",
  "implementation": "0x…",
  "asset_contract": "0x…",
  "unapproved_asset_contract": "0x…",
  "funder": "0x…"
}
```

`run` exits with status zero only when the report's `passed` is true. The report is written to
`--report` and printed to stdout. No seed, signature, or authorization header is included.

## Product test configuration

Run the product in an isolated test environment with:

- the ed25519 public key matching the suite seed, pinned together with the configured `keyid`;
- an empty idempotency and ledger namespace which is not shared with production;
- one approved route named `route` at `route_version` for `chain_id`, whose only approved token is
  `asset_contract`; `unapproved_asset_contract` must not be approved;
- forwarder addresses computed as `forwarder_address(factory, implementation,
  persistent_salt(product_slug, account_id, address_version))`;
- its own RPC connection to the manifest chain, used to verify every cited log;
- the per-deposit cap, the per-period cap, and the period passed to `run`;
- the accounts below, or different ids passed with `--accepted-account-id`,
  `--refused-account-id`, `--processing-account-id`, and `--period-account-id`.

| Account | Required behavior |
|---|---|
| `conformance-accepted` | Creditable. |
| `conformance-refused` | Every settlement returns `200 {"status":"rejected","reason":"…"}` without a ledger credit. |
| `conformance-processing` | Every settlement returns `200 {"status":"processing"}`, retained for `GET`, without a ledger credit. |
| `conformance-period` | Creditable, subject to the per-period cap, and empty when the run starts. |

The literal signing-key value `dev` uses the public test seed `[7; 32]`. A file may instead contain
exactly 32 raw seed bytes or 64 hexadecimal characters. Never use a production settlement seed.

Cap constraints: the per-deposit cap must be at least 201 minor units, the per-period cap at least
2000 and at most 32 per-deposit caps, and the period at least 30 seconds. The per-period case must
finish inside one period: if it takes longer, or the product's period resets during the run, the
case is `incomplete`. Rolling windows avoid resets entirely.

### Required ledger observation hook

A conforming answer alone cannot prove that the ledger mutated exactly once, so the product test
environment must expose this unauthenticated, test-only endpoint next to the settlement URL:

```http
GET {settlement_url}/_conformance/ledger/{account_id}

200 {"balance_minor": "1234", "mutations": 3}
```

`balance_minor` is the sum of all credits to the account in minor units, as a decimal string, and
`mutations` is the number of credits applied. Configured accounts without credits return
`{"balance_minor":"0","mutations":0}`. A `404`, `405`, or `501` means the hook is absent: every
case which needs it is reported `incomplete`. Never expose this hook in production.

### Required restart check

Obligations 2 and 3 require records and the ledger to be durable. `--restart-command` is a shell
command which restarts the product and returns; the suite then waits up to 60 seconds for the
endpoint, re-`GET`s every record the run created, replays the oldest accepted request, and
compares every account's ledger with its pre-restart value. Without `--restart-command` the
`restart_retention` case is `incomplete`.

When the restart cannot be automated, use a command which performs the documented manual step and
blocks until it is done, for example
`--restart-command 'read -p "Restart the product, then press Enter" _ </dev/tty'`. The check is
still validated by re-reading the keys afterward.

## Cases

Every case carries the section 11 obligation it checks; protocol cases have `null`.

| Case id | Obligation | Assertion |
|---|---|---|
| `accepted` | 3 | A valid request returns `200 accepted` with a `destination_tx_id`; an immediate `GET` returns the same answer, and the ledger grew by exactly the amount in one mutation. |
| `replay` | 2 | An exact replay returns the same destination id with no ledger change. |
| `payload_mismatch` | 2 | The same key with a changed payload returns `422`. |
| `concurrency` | 3 | Eight identical parallel requests yield one destination id (others may be `409`) and exactly one ledger mutation. |
| `get_original` | — | `GET` returns the status, destination id, and the original payload. Semantic JSON equality is required; `payload_byte_equal` separately reports whether the raw `payload` member of the `GET` body equals the exact bytes the suite sent, with SHA-256 of both. |
| `authentication` | 1 | Invalid signature bytes, a valid signature under the wrong `keyid`, an expired `created`, and a tampered body are each rejected with `401` or `403`. |
| `idempotency_coverage` | 1 | A request whose signature omits `idempotency-key` is rejected. |
| `per_deposit_cap` | 4 | `cap + 1` minor units is refused. |
| `per_period_cap` | 4 | Distinct deposits credit the period account up to `cap − r` (`r` = the smaller cap); eight concurrent requests of `r` with distinct keys then yield exactly one acceptance, a further request of 1 is refused, and the ledger holds exactly the cap. This fails unless the cumulative check is atomic with the credit. |
| `chain_evidence` | 5 | Five on-chain counter-examples, each claiming the approved token, the account's forwarder, and a consistent deposit id, are refused: a log index past the receipt's logs, a real mint by the unapproved token, a real mint to a different recipient, a claimed amount differing from the real one, and a real log not yet finalized. A finalized valid log is accepted. A product which only checks request fields accepts all five. |
| `deposit_identity` | 6 | A key other than `deposit:` plus the UUIDv5 recomputed from chain id, transaction hash, and log index is refused. |
| `business_refusal` | — | The refused account yields a typed `200 rejected` with a reason and no ledger change. |
| `processing` | — | The processing account yields a typed `200 processing`, retained by `GET`, with no ledger change. |
| `unknown_get` | — | An unknown key returns `404` or `200 {"status":"unknown"}`. |
| `restart_retention` | 2 | After the restart, every record reads back unchanged, the oldest accepted request replays to the same destination id, and no ledger changed. |

## Report format

The schema version is `2`:

```json
{
  "version": 2,
  "settlement_url": "http://127.0.0.1:8080/settlements",
  "manifest": { "version": 1, "chain_id": 31337, "…": "…" },
  "started_at": "2026-09-22T00:00:00Z",
  "finished_at": "2026-09-22T00:00:09Z",
  "passed": true,
  "summary": { "passed": 15, "failed": 0, "incomplete": 0 },
  "tests": [
    {
      "id": "accepted",
      "name": "valid request is committed before accepted and credits the ledger once",
      "obligation": 3,
      "status": "pass",
      "evidence": { "destination_tx_id": "credit-…", "committed_before_response": true }
    }
  ]
}
```

`status` is `pass`, `fail`, or `incomplete`. Evidence is bounded to response codes, destination
ids, ledger observations, hashes, transaction hashes, reasons, and assertion details.

## Reference endpoint

`topup-conformance-reference` is runnable documentation for E2. It verifies signatures with the
same `topup_adapters::http_signature` module the service uses for inbound requests (any label, any
parameter order, optional `alg="ed25519"`), stores the exact payload bytes it received, applies
the cumulative per-period cap and the credit in one critical section (one transaction with a row
lock in PostgreSQL), verifies every cited log against its own RPC, and exposes the ledger hook.

```sh
cargo run --locked -p topup-conformance --all-features --bin topup-conformance-reference -- \
  --manifest target/conformance-manifest.json \
  --listen 127.0.0.1:8089 \
  --signing-key dev \
  --keyid settlement/v1 \
  --per-deposit-cap 10000 \
  --per-period-cap 50000 \
  --period-seconds 86400 \
  --database-url postgres://postgres:postgres@127.0.0.1/conformance
```

`--rpc-url` overrides the manifest's RPC URL for the reference's own verification. Without
`--database-url` (or without the `postgres` feature) state lives in memory, so a real process
restart loses it and `restart_retention` fails, as it should.

`--broken <variant>` (memory storage only) removes exactly one obligation. The integration tests
assert that each variant fails exactly the listed cases and passes every other case:

| Variant | Obligation | Defect | Failing cases |
|---|---|---|---|
| `signature` | 1 | Skips signature verification. | `authentication`, `idempotency_coverage` |
| `idempotency` | 2 | Answers a reused key without comparing payloads. | `payload_mismatch` |
| `retention` | 2 | Keeps idempotency records only in process memory. | `restart_retention` |
| `concurrency` | 3 | Checks for an existing record outside the credit's critical section. | `concurrency` |
| `caps` | 4 | Enforces neither cap. | `per_deposit_cap`, `per_period_cap` |
| `period-cap-race` | 4 | Checks the period cap outside the credit's critical section. | `per_period_cap` |
| `evidence` | 5 | Checks evidence request fields but never consults its RPC. | `chain_evidence` |
| `deposit-identity` | 6 | Does not recompute the deposit id. | `deposit_identity` |

The integration tests in `crates/conformance/tests/reference.rs` start their own Anvil and are
skipped with a message when `anvil` or `forge` is missing; the PostgreSQL case additionally needs
`MIGRATE_DATABASE_URL`. In-process tests model a restart by stopping the server and serving the
same storage again; the PostgreSQL case reconnects with a fresh pool.

The production `Dockerfile` excludes both conformance binaries. They are integration and CI tools;
the distroless runtime image contains only `/usr/local/bin/topup`.
